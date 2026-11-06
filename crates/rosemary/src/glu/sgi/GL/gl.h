/* Private OpenGL ABI subset. No host GL headers or libraries are needed. */
#ifndef ROSEMARY_GLU_GL_H
#define ROSEMARY_GLU_GL_H
#define GLAPIENTRY
#define GLAPI extern
typedef unsigned int GLenum;
typedef unsigned char GLboolean;
typedef unsigned int GLbitfield;
typedef void GLvoid;
typedef signed char GLbyte;
typedef short GLshort;
typedef int GLint;
typedef int GLsizei;
typedef unsigned char GLubyte;
typedef unsigned short GLushort;
typedef unsigned int GLuint;
typedef float GLfloat;
typedef double GLdouble;
typedef double GLclampd;
#define GL_ALPHA 0x1906
#define GL_ARB_texture_cube_map 1
#define GL_BGR 0x80E0
#define GL_BGRA 0x80E1
#define GL_BITMAP 0x1A00
#define GL_BLUE 0x1905
#define GL_BYTE 0x1400
#define GL_COLOR_INDEX 0x1900
#define GL_DEPTH_COMPONENT 0x1902
#define GL_EXT_texture 1
#define GL_EXT_texture3D 1
#define GL_FALSE 0
#define GL_FLOAT 0x1406
#define GL_GREEN 0x1904
#define GL_INT 0x1404
#define GL_LINE_STRIP 0x0003
#define GL_LUMINANCE 0x1909
#define GL_LUMINANCE_ALPHA 0x190A
#define GL_MAX_TEXTURE_SIZE 0x0D33
#define GL_PACK_ALIGNMENT 0x0D05
#define GL_PACK_IMAGE_HEIGHT 0x806C
#define GL_PACK_LSB_FIRST 0x0D01
#define GL_PACK_ROW_LENGTH 0x0D02
#define GL_PACK_SKIP_IMAGES 0x806B
#define GL_PACK_SKIP_PIXELS 0x0D04
#define GL_PACK_SKIP_ROWS 0x0D03
#define GL_PACK_SWAP_BYTES 0x0D00
#define GL_POINTS 0x0000
#define GL_PROXY_TEXTURE_1D 0x8063
#define GL_PROXY_TEXTURE_2D 0x8064
#define GL_PROXY_TEXTURE_3D 0x8070
#define GL_PROXY_TEXTURE_CUBE_MAP_ARB 0x851B
#define GL_QUAD_STRIP 0x0008
#define GL_RED 0x1903
#define GL_RGB 0x1907
#define GL_RGBA 0x1908
#define GL_SHORT 0x1402
#define GL_STENCIL_INDEX 0x1901
#define GL_TEXTURE_1D 0x0DE0
#define GL_TEXTURE_2D 0x0DE1
#define GL_TEXTURE_3D 0x806F
#define GL_TEXTURE_CUBE_MAP_NEGATIVE_X_ARB 0x8516
#define GL_TEXTURE_CUBE_MAP_NEGATIVE_Y_ARB 0x8518
#define GL_TEXTURE_CUBE_MAP_NEGATIVE_Z_ARB 0x851A
#define GL_TEXTURE_CUBE_MAP_POSITIVE_X_ARB 0x8515
#define GL_TEXTURE_CUBE_MAP_POSITIVE_Y_ARB 0x8517
#define GL_TEXTURE_CUBE_MAP_POSITIVE_Z_ARB 0x8519
#define GL_TEXTURE_WIDTH 0x1000
#define GL_TRIANGLES 0x0004
#define GL_TRIANGLE_FAN 0x0006
#define GL_TRUE 1
#define GL_UNPACK_ALIGNMENT 0x0CF5
#define GL_UNPACK_IMAGE_HEIGHT 0x806E
#define GL_UNPACK_LSB_FIRST 0x0CF1
#define GL_UNPACK_ROW_LENGTH 0x0CF2
#define GL_UNPACK_SKIP_IMAGES 0x806D
#define GL_UNPACK_SKIP_PIXELS 0x0CF4
#define GL_UNPACK_SKIP_ROWS 0x0CF3
#define GL_UNPACK_SWAP_BYTES 0x0CF0
#define GL_UNSIGNED_BYTE 0x1401
#define GL_UNSIGNED_BYTE_2_3_3_REV 0x8362
#define GL_UNSIGNED_BYTE_3_3_2 0x8032
#define GL_UNSIGNED_INT 0x1405
#define GL_UNSIGNED_INT_10_10_10_2 0x8036
#define GL_UNSIGNED_INT_2_10_10_10_REV 0x8368
#define GL_UNSIGNED_INT_8_8_8_8 0x8035
#define GL_UNSIGNED_INT_8_8_8_8_REV 0x8367
#define GL_UNSIGNED_SHORT 0x1403
#define GL_UNSIGNED_SHORT_1_5_5_5_REV 0x8366
#define GL_UNSIGNED_SHORT_4_4_4_4 0x8033
#define GL_UNSIGNED_SHORT_4_4_4_4_REV 0x8365
#define GL_UNSIGNED_SHORT_5_5_5_1 0x8034
#define GL_UNSIGNED_SHORT_5_6_5 0x8363
#define GL_UNSIGNED_SHORT_5_6_5_REV 0x8364
#define GL_VERSION 0x1F02
#define gluBuild1DMipmapLevels rosemary_gluBuild1DMipmapLevels
#define gluBuild1DMipmapLevelsCore rosemary_gluBuild1DMipmapLevelsCore
#define gluBuild1DMipmaps rosemary_gluBuild1DMipmaps
#define gluBuild2DMipmapLevels rosemary_gluBuild2DMipmapLevels
#define gluBuild2DMipmapLevelsCore rosemary_gluBuild2DMipmapLevelsCore
#define gluBuild2DMipmaps rosemary_gluBuild2DMipmaps
#define gluBuild3DMipmapLevels rosemary_gluBuild3DMipmapLevels
#define gluBuild3DMipmapLevelsCore rosemary_gluBuild3DMipmapLevelsCore
#define gluBuild3DMipmaps rosemary_gluBuild3DMipmaps
#define gluCylinder rosemary_gluCylinder
#define gluDeleteQuadric rosemary_gluDeleteQuadric
#define gluDisk rosemary_gluDisk
#define gluLookAt rosemary_gluLookAt
#define gluNewQuadric rosemary_gluNewQuadric
#define gluOrtho2D rosemary_gluOrtho2D
#define gluPartialDisk rosemary_gluPartialDisk
#define gluPerspective rosemary_gluPerspective
#define gluPickMatrix rosemary_gluPickMatrix
#define gluProject rosemary_gluProject
#define gluQuadricCallback rosemary_gluQuadricCallback
#define gluQuadricDrawStyle rosemary_gluQuadricDrawStyle
#define gluQuadricError rosemary_gluQuadricError
#define gluQuadricNormals rosemary_gluQuadricNormals
#define gluQuadricOrientation rosemary_gluQuadricOrientation
#define gluQuadricTexture rosemary_gluQuadricTexture
#define gluScaleImage rosemary_gluScaleImage
#define gluScaleImage3D rosemary_gluScaleImage3D
#define gluSphere rosemary_gluSphere
#define gluUnProject rosemary_gluUnProject
#define gluUnProject4 rosemary_gluUnProject4
#define glBegin rosemary_glu_glBegin
void rosemary_glu_glBegin(unsigned int mode);
#define glEnd rosemary_glu_glEnd
void rosemary_glu_glEnd(void);
#define glNormal3f rosemary_glu_glNormal3f
void rosemary_glu_glNormal3f(float x, float y, float z);
#define glTexCoord2f rosemary_glu_glTexCoord2f
void rosemary_glu_glTexCoord2f(float s, float t);
#define glVertex3f rosemary_glu_glVertex3f
void rosemary_glu_glVertex3f(float x, float y, float z);
#define glMultMatrixd rosemary_glu_glMultMatrixd
void rosemary_glu_glMultMatrixd(const double * m);
#define glMultMatrixf rosemary_glu_glMultMatrixf
void rosemary_glu_glMultMatrixf(const float * m);
#define glOrtho rosemary_glu_glOrtho
void rosemary_glu_glOrtho(double left, double right, double bottom, double top, double near, double far);
#define glScalef rosemary_glu_glScalef
void rosemary_glu_glScalef(float x, float y, float z);
#define glTranslatef rosemary_glu_glTranslatef
void rosemary_glu_glTranslatef(float x, float y, float z);
#define glTranslated rosemary_glu_glTranslated
void rosemary_glu_glTranslated(double x, double y, double z);
#define glGetIntegerv rosemary_glu_glGetIntegerv
void rosemary_glu_glGetIntegerv(unsigned int name, int * value);
#define glGetString rosemary_glu_glGetString
const unsigned char * rosemary_glu_glGetString(unsigned int name);
#define glGetTexLevelParameteriv rosemary_glu_glGetTexLevelParameteriv
void rosemary_glu_glGetTexLevelParameteriv(unsigned int target, int level, unsigned int name, int * value);
#define glPixelStorei rosemary_glu_glPixelStorei
void rosemary_glu_glPixelStorei(unsigned int name, int value);
#define glTexImage1D rosemary_glu_glTexImage1D
void rosemary_glu_glTexImage1D(unsigned int target, int level, int internal, int width, int border, unsigned int format, unsigned int kind, const void * pixels);
#define glTexImage2D rosemary_glu_glTexImage2D
void rosemary_glu_glTexImage2D(unsigned int target, int level, int internal, int width, int height, int border, unsigned int format, unsigned int kind, const void * pixels);
#define glTexImage3D rosemary_glu_glTexImage3D
void rosemary_glu_glTexImage3D(unsigned int target, int level, int internal, int width, int height, int depth, int border, unsigned int format, unsigned int kind, const void * pixels);
#endif
